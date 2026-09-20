//! Public SDK access, including the interface versions used by older Mac games.
//! The game's own flat wrappers perform C++ calls; no private engine ABI is used.
use crate::{Result, native::Library, symbol};
use std::ffi::{CStr, c_char, c_void};

pub(super) enum Interface {
    User,
    Apps,
    Utils,
    Stats,
    Storage,
}

pub(super) struct Api {
    library: Library,
    legacy: bool,
    shutdown: unsafe extern "C" fn(),
}

// Original i386/x86_64 libsteam_api.dylib observed in the verified Dead Cells
// installation. Recognition is of SDK bytes, never a game AppID exception.
const LEGACY_CLIENT017_SIZE: u64 = 609_216;
const LEGACY_CLIENT017_SHA256: &str =
    "41dc3afbe16368e0e54624d561ed45ab85186e7e8641176d81dfdf4b414068f9";

enum Profile {
    Modern,
    LegacyClient017,
}

impl Api {
    pub fn initialize(library: Library, path: &std::path::Path) -> Result<Self> {
        let profile = if library.optional_symbol(c"SteamAPI_InitFlat").is_some() {
            Profile::Modern
        } else if std::fs::metadata(path)?.len() == LEGACY_CLIENT017_SIZE
            && crate::sha(path)? == LEGACY_CLIENT017_SHA256
        {
            Profile::LegacyClient017
        } else {
            return Err("Unrecognized legacy Steamworks SDK; refusing interface calls".into());
        };
        Self::initialize_profile(library, profile)
    }

    // The fixture exercises the real initialization/interface calls with an
    // explicitly supplied profile. Production always goes through fingerprinting.
    #[cfg(test)]
    pub fn initialize_legacy_fixture(library: Library) -> Result<Self> {
        Self::initialize_profile(library, Profile::LegacyClient017)
    }

    fn initialize_profile(library: Library, profile: Profile) -> Result<Self> {
        // Resolve teardown before initialization, so every successful init has it.
        let shutdown = symbol!(&library, c"SteamAPI_Shutdown", unsafe extern "C" fn());
        let legacy = if matches!(profile, Profile::Modern) {
            let initialize = symbol!(
                &library,
                c"SteamAPI_InitFlat",
                unsafe extern "C" fn(*mut c_char) -> i32
            );
            let mut error = [0_i8; 1024];
            // SAFETY: writable error buffer matches the documented size.
            if unsafe { initialize(error.as_mut_ptr()) } != 0 {
                return Err("Steamworks initialization failed".into());
            }
            false
        } else {
            let initialize = symbol!(&library, c"SteamAPI_Init", unsafe extern "C" fn() -> bool);
            // SAFETY: historical public C entry point takes no arguments.
            if !unsafe { initialize() } {
                return Err("Steamworks initialization failed".into());
            }
            true
        };
        Ok(Self {
            library,
            legacy,
            shutdown,
        })
    }

    pub fn symbol(&self, name: &CStr) -> Result<*mut c_void> {
        self.library.symbol(name)
    }

    pub fn interface(&self, interface: Interface) -> Result<*mut c_void> {
        let (modern, getter, version) = match interface {
            Interface::User => (
                c"SteamAPI_SteamUser_v023",
                c"SteamAPI_ISteamClient_GetISteamUser",
                c"SteamUser019",
            ),
            Interface::Apps => (
                c"SteamAPI_SteamApps_v008",
                c"SteamAPI_ISteamClient_GetISteamApps",
                c"STEAMAPPS_INTERFACE_VERSION008",
            ),
            Interface::Utils => (
                c"SteamAPI_SteamUtils_v010",
                c"SteamAPI_ISteamClient_GetISteamUtils",
                c"SteamUtils009",
            ),
            Interface::Stats => (
                c"SteamAPI_SteamUserStats_v012",
                c"SteamAPI_ISteamClient_GetISteamUserStats",
                c"STEAMUSERSTATS_INTERFACE_VERSION011",
            ),
            Interface::Storage => (
                c"SteamAPI_SteamRemoteStorage_v016",
                c"SteamAPI_ISteamClient_GetISteamRemoteStorage",
                c"STEAMREMOTESTORAGE_INTERFACE_VERSION014",
            ),
        };
        let object = if self.legacy {
            // SteamClient() returns the client version selected by this SDK's init.
            // These older interface versions match the legacy flat wrappers. In
            // particular Storage014 and Storage016 have different method slots.
            let get_client = symbol!(self, c"SteamClient", unsafe extern "C" fn() -> *mut c_void);
            let get_user = symbol!(
                self,
                c"SteamAPI_GetHSteamUser",
                unsafe extern "C" fn() -> i32
            );
            let get_pipe = symbol!(
                self,
                c"SteamAPI_GetHSteamPipe",
                unsafe extern "C" fn() -> i32
            );
            // SAFETY: initialized SDK's documented public C accessors.
            let (client, user, pipe) = unsafe { (get_client(), get_user(), get_pipe()) };
            if client.is_null() || user == 0 || pipe == 0 {
                return Err("Missing Steamworks client handles".into());
            }
            if matches!(interface, Interface::Utils) {
                let get = symbol!(
                    self,
                    getter,
                    unsafe extern "C" fn(*mut c_void, i32, *const c_char) -> *mut c_void
                );
                // SAFETY: public Utils accessor has a pipe, but no user argument.
                unsafe { get(client, pipe, version.as_ptr()) }
            } else {
                let get = symbol!(
                    self,
                    getter,
                    unsafe extern "C" fn(*mut c_void, i32, i32, *const c_char) -> *mut c_void
                );
                // SAFETY: public user interface accessor; checked handles/version.
                unsafe { get(client, user, pipe, version.as_ptr()) }
            }
        } else {
            let get = symbol!(self, modern, unsafe extern "C" fn() -> *mut c_void);
            // SAFETY: public versioned C accessor for the initialized modern SDK.
            unsafe { get() }
        };
        if object.is_null() {
            return Err("Missing Steamworks interface".into());
        }
        Ok(object)
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        // SAFETY: resolved before successful initialization, called once by owner.
        unsafe { (self.shutdown)() }
    }
}
