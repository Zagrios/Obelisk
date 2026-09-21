//! Process-lifetime dynamic libraries. Steam's internal unload ABI is unknown.
use crate::Result;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Replace this architecture-specific helper with the selected slice of a game.
/// No shell or platform wrapper gets a chance to strip the game's dyld variables.
pub fn exec_game(arguments: &[std::ffi::OsString]) -> std::io::Result<()> {
    use std::{
        fs::File,
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
    };
    // The supervisor treats EOF as successful exec. Retain a separate descriptor
    // that closes atomically on exec, while the game itself gets null stdout.
    // SAFETY: duplicate this helper's inherited stdout; ownership transfers below.
    let descriptor = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        let _ = std::io::stdout().write_all(b"exec-failed\n");
        return Err(error);
    }
    // SAFETY: successful fcntl returned a new owned descriptor.
    let mut control = unsafe { File::from_raw_fd(descriptor) };
    let result = (|| {
        let null = File::options().read(true).write(true).open("/dev/null")?;
        // SAFETY: valid null descriptor, redirecting only this helper's stdout.
        if unsafe { libc::dup2(null.as_raw_fd(), 1) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The supervisor first persists ownership of this PID, then grants exec.
        // Losing that parent before its acknowledgement must never launch a game.
        let mut admission = [0];
        std::io::stdin().read_exact(&mut admission)?;
        if admission != *b"1" {
            return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
        }
        // SAFETY: valid readable null descriptor; the game must not inherit the
        // private admission pipe or any bytes remaining in it.
        if unsafe { libc::dup2(null.as_raw_fd(), 0) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        exec_game_inner(arguments)
    })();
    // Successful SETEXEC cannot reach this write. Normal setup/exec failures
    // always send an explicit bounded marker before closing the control pipe.
    let _ = control.write_all(b"exec-failed\n");
    result
}

fn exec_game_inner(arguments: &[std::ffi::OsString]) -> std::io::Result<()> {
    let strings = arguments
        .iter()
        .map(|argument| CString::new(argument.as_os_str().as_bytes()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let executable = strings
        .first()
        .ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let mut argv: Vec<_> = strings.iter().map(|s| s.as_ptr().cast_mut()).collect();
    argv.push(std::ptr::null_mut());
    let mut attributes = std::ptr::null_mut();
    // SAFETY: writable opaque attribute storage, initialized before any use.
    let status = unsafe { libc::posix_spawnattr_init(&mut attributes) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status));
    }
    let result = (|| {
        // CPU_ARCH_ABI64 | CPU_TYPE_X86/ARM, from mach/machine.h.
        #[cfg(target_arch = "x86_64")]
        let mut architecture = 0x0100_0007;
        #[cfg(target_arch = "aarch64")]
        let mut architecture = 0x0100_000c;
        let mut accepted = 0;
        // SAFETY: initialized attributes and exactly one valid CPU preference.
        let status = unsafe {
            libc::posix_spawnattr_setbinpref_np(
                &mut attributes,
                1,
                &mut architecture,
                &mut accepted,
            )
        };
        if status != 0 || accepted != 1 {
            return Err(std::io::Error::from_raw_os_error(if status == 0 {
                libc::EINVAL
            } else {
                status
            }));
        }
        // SAFETY: SETEXEC replaces this PID, retaining supervisor parentage.
        let status = unsafe {
            libc::posix_spawnattr_setflags(&mut attributes, libc::POSIX_SPAWN_SETEXEC as i16)
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status));
        }
        unsafe extern "C" {
            fn _NSGetEnviron() -> *mut *mut *mut c_char;
        }
        // SAFETY: argv is NUL terminated and all owned strings remain live. This
        // startup path has not modified the inherited environment or cwd. The
        // process environment is passed verbatim, including non-UTF8 values.
        let status = unsafe {
            libc::posix_spawn(
                std::ptr::null_mut(),
                executable.as_ptr(),
                std::ptr::null(),
                &attributes,
                argv.as_ptr(),
                *_NSGetEnviron(),
            )
        };
        // Successful SETEXEC never returns. Refuse an unexpected returned zero.
        Err(std::io::Error::from_raw_os_error(if status == 0 {
            libc::EIO
        } else {
            status
        }))
    })();
    // SAFETY: attribute initialization succeeded; only failed exec returns here.
    unsafe { libc::posix_spawnattr_destroy(&mut attributes) };
    result
}

pub struct Library(*mut c_void);
impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: NUL-terminated path; the handle is deliberately retained until exit.
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            // Do not print native error strings, which may contain private data.
            return Err("dlopen failed".into());
        }
        Ok(Self(handle))
    }

    pub fn symbol(&self, name: &CStr) -> Result<*mut c_void> {
        self.optional_symbol(name)
            .ok_or_else(|| format!("missing export: {}", name.to_string_lossy()).into())
    }

    fn optional_symbol(&self, name: &CStr) -> Option<*mut c_void> {
        // SAFETY: valid open handle and NUL-terminated symbol name.
        let pointer = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        (!pointer.is_null()).then_some(pointer)
    }
}

/// Resolve a known C ABI signature. Each invocation must document the signature.
#[macro_export]
macro_rules! symbol {
    ($image:expr, $name:expr, $signature:ty) => {{
        let address = $image.symbol($name)?;
        // SAFETY: the call site supplies the export's verified C ABI signature;
        // macOS function and data pointers have the same representation.
        unsafe { std::mem::transmute::<*mut std::ffi::c_void, $signature>(address) }
    }};
}

pub fn disable_core_dumps() -> Result<()> {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: pointer to an initialized rlimit; affects only this process.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub fn translated() -> bool {
    let mut value = 0_i32;
    let mut size = std::mem::size_of_val(&value);
    // SAFETY: matching writable scalar/size and a fixed sysctl name.
    unsafe {
        libc::sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            (&mut value as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        );
    }
    value != 0
}

pub fn parent_pid() -> i32 {
    // SAFETY: getppid takes no pointers and cannot fail.
    unsafe { libc::getppid() }
}

/// Kernel birth time prevents adopting a different process after PID reuse.
pub fn process_identity(pid: u32) -> Option<steam_runtime::supervisor::Identity> {
    let pid = i32::try_from(pid).ok()?;
    // SAFETY: proc_bsdinfo is a C POD; the buffer length matches its allocation.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: initialized writable buffer and documented libproc selector.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    // SAFETY: geteuid takes no pointers and cannot fail.
    if read != size || info.pbi_uid != unsafe { libc::geteuid() } {
        return None;
    }
    let mut path = [0_u8; 4096];
    // SAFETY: writable byte buffer with its exact capacity, fixed libproc API.
    let count = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if count <= 0 {
        return None;
    }
    let end = path.iter().position(|&b| b == 0)?;
    Some(steam_runtime::supervisor::Identity {
        pid: pid as u32,
        uid: info.pbi_uid,
        birth_seconds: info.pbi_start_tvsec,
        birth_microseconds: info.pbi_start_tvusec,
        executable: std::str::from_utf8(&path[..end]).ok()?.into(),
    })
}

pub fn detach_supervisor() -> Result<()> {
    // SAFETY: setsid affects only this freshly spawned helper. It has no terminal
    // or UI log descriptors (stderr carries a private launch acknowledgement).
    // It must survive its parent's termination.
    if unsafe { libc::setsid() } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// A failed identity read is only treated as exit if the kernel says ESRCH.
pub fn process_is_absent(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal zero only queries existence; it never signals the process.
    (unsafe { libc::kill(pid, 0) }) < 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(test)]
mod process_tests {
    use super::*;
    #[test]
    fn kernel_identity_distinguishes_current_process_from_exited_child() {
        let own = process_identity(std::process::id()).unwrap();
        assert_eq!(own.pid, std::process::id());
        assert!(own.birth_seconds > 0);
        assert_eq!(process_identity(own.pid), Some(own));
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0.1")
            .spawn()
            .unwrap();
        let identity = process_identity(child.id()).unwrap();
        assert_ne!(identity.pid, std::process::id());
        assert!(!process_is_absent(child.id()));
        child.wait().unwrap();
        assert!(process_is_absent(child.id()));
        assert!(process_identity(child.id()).is_none());
    }
}
