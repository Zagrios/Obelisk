//! Process-lifetime dynamic libraries. Steam's internal unload ABI is unknown.
use crate::Result;
use std::ffi::{CStr, CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
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
        // SAFETY: valid open handle and NUL-terminated symbol name.
        let pointer = unsafe { libc::dlsym(self.0, name.as_ptr()) };
        if pointer.is_null() {
            return Err(format!("missing export: {}", name.to_string_lossy()).into());
        }
        Ok(pointer)
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

pub fn loaded_images() -> Vec<String> {
    // SAFETY: dyld owns the NUL-terminated image names for loaded process images.
    unsafe {
        (0.._dyld_image_count())
            .filter_map(|i| {
                let name = _dyld_get_image_name(i);
                (!name.is_null()).then(|| CStr::from_ptr(name).to_string_lossy().into_owned())
            })
            .collect()
    }
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
