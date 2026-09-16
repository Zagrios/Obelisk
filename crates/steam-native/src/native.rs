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
