use sailover_steam_native::native;
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const FIXTURE: &str = "SAILOVER_SUPERVISOR_SIGNAL_FIXTURE";

fn current_mask() -> libc::sigset_t {
    // SAFETY: a null input set only queries the calling thread's signal mask;
    // the output points to initialized, correctly sized storage.
    unsafe {
        let mut mask = std::mem::zeroed();
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask),
            0
        );
        mask
    }
}

#[test]
fn supervisor_unblocks_signals_inherited_from_its_launcher() {
    if std::env::var_os(FIXTURE).is_some() {
        let before = current_mask();
        // SAFETY: the mask is initialized and both signals are valid on macOS.
        unsafe {
            assert_eq!(libc::sigismember(&before, libc::SIGCHLD), 1);
            assert_eq!(libc::sigismember(&before, libc::SIGTERM), 1);
        }
        native::detach_supervisor().unwrap();
        let after = current_mask();
        // Darwin's signal mask is a bit set; the standalone supervisor must
        // receive signals even when its launching thread deliberately blocked them.
        assert_eq!(after, 0, "supervisor retained its launcher's signal mask");
        return;
    }

    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "supervisor_unblocks_signals_inherited_from_its_launcher",
            "--nocapture",
        ])
        .env(FIXTURE, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: this runs only in the new fixture process before exec. The
    // closure uses async-signal-safe libc calls and changes no parent state.
    unsafe {
        command.pre_exec(|| {
            let mut mask = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, libc::SIGCHLD);
            libc::sigaddset(&mut mask, libc::SIGTERM);
            let status = libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
            if status == 0 {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(status))
            }
        });
    }
    let mut fixture = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            fixture.kill().unwrap();
            fixture.wait().unwrap();
            panic!("isolated supervisor signal fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = fixture.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "isolated fixture failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
