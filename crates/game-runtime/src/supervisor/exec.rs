//! Exec admission. The supervisor spawns an exec helper, records that process
//! as the Game session's durable `starting` ownership, then admits it with one
//! message on its stdin. The helper becomes the game only after a complete
//! message: losing the supervisor first closes the pipe and prevents exec.
//! Becoming the game closes the helper's stdout, which confirms it; a helper
//! that could not replies `FAILED` there instead.
//!
//! A confirmed exec starts the game; only reaping that child ends it.
//! Inspection supplies durable identities, never permission to abandon a child.
use super::{Error, Identity, StartingGame, Target};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::Infallible,
    io::{self, BufRead, Read, Write},
    os::{
        fd::OwnedFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::Duration,
};
use tokio::{io::AsyncReadExt, time::Instant};

/// The subcommand that makes Obelisk's host an exec helper:
/// `obelisk-steam-host game-exec`.
pub const SUBCOMMAND: &str = "game-exec";

/// Reply of an exec helper whose preparation or exec failed. A helper that
/// replaced itself with the game instead closes its confirmation pipe silently.
pub const FAILED: &[u8] = b"exec-failed\n";

/// The game to become. Its environment is the helper's own, set when the
/// supervisor spawned it, so exec passes it on verbatim.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Admission {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    #[serde(default)]
    pub log: Option<PathBuf>,
    /// What the log starts with, before the game writes to it.
    #[serde(default)]
    pub log_header: Option<String>,
}

impl Admission {
    /// The game `target` names, whose log starts with `log_header`.
    pub(crate) fn of(target: &Target, log_header: Option<String>) -> Self {
        Self {
            executable: target.executable.clone(),
            arguments: target.arguments.clone(),
            working_directory: target.working_directory.clone(),
            log: target.log.clone(),
            log_header,
        }
    }
}

/// Game arguments are bounded like the kernel's ARG_MAX, not like a record.
const MAX: u64 = 1024 * 1024;

pub fn send(output: &mut impl Write, admission: &Admission) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(admission).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    output.write_all(&bytes)?;
    output.flush()
}

/// Reads one complete admission. A truncated or unbounded message, or a
/// relative path, is refused.
fn receive(input: impl Read) -> io::Result<Admission> {
    let mut bytes = Vec::new();
    io::BufReader::new(input.take(MAX + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > MAX || bytes.last() != Some(&b'\n') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let admission: Admission =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if !admission.executable.is_absolute()
        || !admission.working_directory.is_absolute()
        || admission.log.as_ref().is_some_and(|log| !log.is_absolute())
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(admission)
}

/// The helper's side: becomes the game its supervisor admits on `input`, and
/// returns only when it could not, after replying `FAILED`. `std` exec
/// restores the default SIGPIPE disposition that the Rust runtime ignores,
/// and leaves the environment untouched.
pub fn game_exec(input: impl Read) -> io::Error {
    let mut confirmation = match confirmation() {
        Ok(confirmation) => confirmation,
        Err(error) => return error,
    };
    let Err(error) = exec(input);
    let _ = confirmation.write_all(FAILED);
    error
}

/// A CLOEXEC duplicate of stdout that closes atomically on a successful exec,
/// which the supervisor reads as EOF. When even that fails, the marker still
/// reaches the supervisor through stdout.
fn confirmation() -> io::Result<std::fs::File> {
    use std::os::fd::AsFd;
    let stdout = io::stdout();
    stdout
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .inspect_err(|_| {
            let _ = stdout.lock().write_all(FAILED);
        })
}

fn exec(input: impl Read) -> io::Result<Infallible> {
    let admission = receive(input)?;
    Err(Command::new(&admission.executable)
        .args(&admission.arguments)
        .current_dir(&admission.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(standard_error(&admission)?)
        .exec())
}

/// The game's standard error: its log, emptied for this launch and started
/// with its header, or nothing.
fn standard_error(admission: &Admission) -> io::Result<Stdio> {
    let Some(log) = &admission.log else {
        return Ok(Stdio::null());
    };
    let mut log = crate::diagnostics::open_log(log)?;
    if let Some(header) = &admission.log_header {
        log.write_all(header.as_bytes())?;
    }
    Ok(log.into())
}

/// The supervisor's side: an exec helper ready to spawn, with the game it
/// will be admitted to become.
pub(crate) struct Helper {
    command: Command,
    pipe: UnixStream,
    admission: Admission,
    /// The game's executable, as inspection names it once the helper became it.
    expected: PathBuf,
}

impl Helper {
    /// `program` is Obelisk's host, and `environment` the game's: it is set
    /// on the helper and inherited through exec, so it reaches the game
    /// verbatim.
    pub(crate) fn new(
        program: &Path,
        environment: &HashMap<String, String>,
        admission: Admission,
    ) -> Result<Self, Error> {
        let (pipe, input) = UnixStream::pair()?;
        pipe.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut command = Command::new(program);
        command
            .arg(SUBCOMMAND)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::from(OwnedFd::from(input)))
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let expected = admission
            .executable
            .canonicalize()
            .map_err(|_| Error::StartFailed)?;
        Ok(Self {
            command,
            pipe,
            admission,
            expected,
        })
    }

    /// Spawns the helper, which waits for its admission.
    pub(crate) fn spawn(self) -> Result<Child, Error> {
        let child = tokio::process::Command::from(self.command)
            .kill_on_drop(false)
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        Ok(Child {
            child,
            pipe: Some(self.pipe),
            admission: self.admission,
            expected: self.expected,
        })
    }
}

/// The supervisor's child: an exec helper, then the game it became, until it
/// is reaped.
pub(crate) struct Child {
    child: tokio::process::Child,
    /// Open until the helper is admitted, or never will be.
    pipe: Option<UnixStream>,
    admission: Admission,
    expected: PathBuf,
}

impl Child {
    /// `None` once reaped.
    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Admits the helper and returns its pid: `record` first makes its
    /// identity and its destination durable, then the message lets it become
    /// the game. Without a complete admission the helper exits without exec.
    pub(crate) fn admit(
        &mut self,
        inspect: impl Fn(u32) -> Result<Option<Identity>, Error>,
        record: impl FnOnce(StartingGame) -> Result<(), Error>,
    ) -> Result<u32, Error> {
        let mut pipe = self.pipe.take().ok_or(Error::StartFailed)?;
        // Read now: a game reaped during confirmation no longer has one.
        let pid = self.child.id().ok_or(Error::StartFailed)?;
        let identity = inspect(pid)?.ok_or(Error::StartFailed)?;
        // The helper cannot exec before its identity and destination are
        // durable: this write must succeed.
        record(StartingGame::Process {
            identity,
            executable: self.expected.clone(),
        })?;
        send(&mut pipe, &self.admission).map_err(|_| Error::StartFailed)?;
        Ok(pid)
    }

    /// Whether the admitted helper became the game.
    pub(crate) async fn confirm(
        &mut self,
        inspect: impl Fn(u32) -> Result<Option<Identity>, Error>,
    ) -> Execution {
        let inspect = |pid| inspect(pid).ok().flatten();
        confirm_execution(&mut self.child, &self.admission.executable, inspect).await
    }

    /// The running game's identity, once inspection names its executable.
    pub(crate) async fn identity(
        &mut self,
        inspect: impl Fn(u32) -> Result<Option<Identity>, Error>,
    ) -> Option<Identity> {
        let inspect = |pid| inspect(pid).ok().flatten();
        wait_identity(&mut self.child, &self.admission.executable, inspect).await
    }

    /// Reaps the child: its exit code, or the signal that ended it.
    pub(crate) async fn exit(&mut self) -> ExitStatus {
        wait_for_exit(&mut self.child).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Execution {
    Started,
    Failed,
    ExitedUnconfirmed,
}

/// How the child ended: its exit code, or the signal that ended it.
async fn wait_for_exit(child: &mut tokio::process::Child) -> ExitStatus {
    loop {
        match child.wait().await {
            Ok(status) => return status,
            // Losing an observation never releases ownership of a live game.
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// The signed helper owns a CLOEXEC pipe: EOF confirms replacement by the game,
/// while ordinary preparation/exec failures send an explicit bounded marker.
async fn confirm_execution(
    child: &mut tokio::process::Child,
    executable: &Path,
    inspect: impl Fn(u32) -> Option<Identity>,
) -> Execution {
    let pid = child.id();
    let expected = executable.canonicalize().ok();
    let observing = Instant::now();
    let output = child.stdout.take();
    let read = async move {
        let Some(output) = output else {
            return std::future::pending::<io::Result<Vec<u8>>>().await;
        };
        let mut bytes = Vec::new();
        output.take(64).read_to_end(&mut bytes).await?;
        Ok(bytes)
    };
    tokio::pin!(read);
    let mut received = false;
    loop {
        tokio::select! {
            biased;
            result = &mut read, if !received => {
                received = true;
                if let Some(execution) = execution_reply(result) {
                    return execution;
                }
            }
            result = child.wait() => {
                if result.is_ok() {
                    // Reaping and pipe EOF can become ready together. Consume
                    // the final reply before rejecting a very short game.
                    if !received
                        && let Ok(result) = tokio::time::timeout(
                            Duration::from_millis(100), &mut read,
                        ).await
                        && let Some(execution) = execution_reply(result)
                    {
                        return execution;
                    }
                    return Execution::ExitedUnconfirmed;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {
                if observing.elapsed() >= Duration::from_secs(3)
                    && pid.and_then(&inspect).is_some_and(|identity| {
                        Some(Path::new(&identity.executable)) == expected.as_deref()
                    })
                {
                    return Execution::Started;
                }
            }
        }
    }
}

fn execution_reply(result: io::Result<Vec<u8>>) -> Option<Execution> {
    match result {
        Ok(bytes) if bytes.is_empty() => Some(Execution::Started),
        Ok(bytes) if bytes == FAILED => Some(Execution::Failed),
        _ => None,
    }
}

/// Never persist the exec helper as the running game's identity.
async fn wait_identity(
    child: &mut tokio::process::Child,
    executable: &Path,
    inspect: impl Fn(u32) -> Option<Identity>,
) -> Option<Identity> {
    let pid = child.id()?;
    let expected = executable.canonicalize().ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(identity) = inspect(pid)
            && Path::new(&identity.executable) == expected
        {
            return Some(identity);
        }
        tokio::select! {
            result = child.wait() => {
                if result.is_ok() {
                    return None;
                }
            }
            _ = tokio::time::sleep_until(deadline) => return None,
            _ = tokio::time::sleep(Duration::from_millis(5)) => (),
        }
    }
}

#[cfg(test)]
mod tests;
