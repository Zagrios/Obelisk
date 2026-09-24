use crate::Result;
use game_runtime::protocol::Event;
use std::{fs::File, io::Write};

/// Reserve the original stdout for our typed events, then discard all native
/// stdout/stderr before loading Valve. The private FD cannot reach a child exec.
pub fn event_pipe() -> Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let null = File::options().write(true).open("/dev/null")?;
    // SAFETY: duplicate a valid pipe descriptor into an owned File, set close-on-
    // exec, and redirect only this helper's standard output/error descriptors.
    unsafe {
        let fd = libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3);
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let output = File::from_raw_fd(fd);
        if libc::dup2(null.as_raw_fd(), 1) < 0 || libc::dup2(null.as_raw_fd(), 2) < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(output)
    }
}
pub fn emit(output: &mut File, event: &Event) -> Result<()> {
    serde_json::to_writer(&mut *output, event)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
