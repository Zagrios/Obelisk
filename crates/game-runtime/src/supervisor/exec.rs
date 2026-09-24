//! Exec admission. The supervisor spawns an exec helper, records that process
//! as the Game session's durable `starting` ownership, then admits it with one
//! message on its stdin. The helper becomes the game only after a complete
//! message: losing the supervisor first closes the pipe and prevents exec.
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, Read, Write},
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
};

/// The game to become. Its environment is the helper's own, set when the
/// supervisor spawned it, so exec passes it on verbatim.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Admission {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
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
pub fn receive(input: impl Read) -> io::Result<Admission> {
    let mut bytes = Vec::new();
    io::BufReader::new(input.take(MAX + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > MAX || bytes.last() != Some(&b'\n') {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let admission: Admission =
        serde_json::from_slice(&bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if !admission.executable.is_absolute() || !admission.working_directory.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(admission)
}

/// The arm64 helper. `std` exec restores the default SIGPIPE disposition
/// that the Rust runtime ignores, and leaves the environment untouched.
pub fn game_exec(input: impl Read) -> io::Result<()> {
    crate::protocol::exec_or_report(|| exec(input))
}

fn exec(input: impl Read) -> io::Result<()> {
    let admission = receive(input)?;
    Err(Command::new(&admission.executable)
        .args(&admission.arguments)
        .current_dir(&admission.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .exec())
}
