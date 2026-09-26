//! Redirect exactly one bootstrap lookup; never replace any Steamworks result.
#[cfg(not(all(
    target_os = "macos",
    any(target_arch = "aarch64", target_arch = "x86_64")
)))]
compile_error!("The discovery adapter requires 64-bit macOS");
use std::ffi::{CStr, c_char};
type Lookup = unsafe extern "C" fn(u32, *const c_char, *mut u32) -> i32;
unsafe extern "C" {
    fn bootstrap_look_up(port: u32, name: *const c_char, result: *mut u32) -> i32;
}
unsafe extern "C" fn lookup(port: u32, name: *const c_char, result: *mut u32) -> i32 {
    // SAFETY: dyld interposing preserves bootstrap_look_up's contract. The
    // environment is inherited before exec and remains unchanged by this image.
    // Calling the original symbol from the interposing image does not recurse.
    unsafe {
        let service = libc::getenv(c"SAILOVER_PRIVATE_STEAM_DISCOVERY".as_ptr());
        if !service.is_null()
            && !name.is_null()
            && CStr::from_ptr(service)
                .to_bytes()
                .starts_with(b"com.zagrios.sailover.discovery.")
            && CStr::from_ptr(name) == c"com.valvesoftware.steam.ipctool"
        {
            return bootstrap_look_up(port, service, result);
        }
        bootstrap_look_up(port, name, result)
    }
}
#[repr(C)]
struct Interpose {
    replacement: Lookup,
    original: Lookup,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE: Interpose = Interpose {
    replacement: lookup,
    original: bootstrap_look_up,
};
