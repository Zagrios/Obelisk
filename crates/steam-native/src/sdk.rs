//! Read-only verification against the game's original Valve Steamworks library.
use crate::{
    Result,
    native::{self, Library},
    symbol,
};
use std::{
    ffi::{c_char, c_void},
    io::Read,
    time::{Duration, Instant},
};
use steam_runtime::protocol::SdkStage;
macro_rules! call {
    ($f:ident($($arg:expr),* $(,)?)) => {{
        // SAFETY: flat Steamworks C signatures; checked non-null interfaces and
        // writable buffers matching the official 64-bit layouts.
        unsafe { $f($($arg),*) }
    }};
}
#[repr(C)]
#[derive(Default)]
struct Players {
    success: u8,
    count: i32,
}
const _: () = assert!(std::mem::size_of::<Players>() == 8);
pub fn verify(
    api_path: &std::path::Path,
    engine_path: &std::path::Path,
    app: u32,
    mut stage: impl FnMut(SdkStage) -> Result<()>,
) -> Result<bool> {
    crate::pinned_client(engine_path)?;
    let mut identity = [0; 8];
    std::io::stdin().read_exact(&mut identity)?;
    let expected = u64::from_le_bytes(identity);
    if expected == 0 || app == 0 {
        return Err("Invalid SDK verification request".into());
    }
    stage(SdkStage::Library)?;
    let api = Library::open(api_path)?;
    let init = symbol!(
        api,
        c"SteamAPI_InitFlat",
        unsafe extern "C" fn(*mut c_char) -> i32
    );
    stage(SdkStage::Initialize)?;
    let mut error = [0_i8; 1024];
    if call!(init(error.as_mut_ptr())) != 0 {
        return Err("Steamworks initialization failed".into());
    }
    let result = verify_interfaces(&api, engine_path, expected, app, &mut stage);
    let shutdown = symbol!(api, c"SteamAPI_Shutdown", unsafe extern "C" fn());
    call!(shutdown());
    result
}
fn verify_interfaces(
    api: &Library,
    engine_path: &std::path::Path,
    expected: u64,
    app: u32,
    stage: &mut impl FnMut(SdkStage) -> Result<()>,
) -> Result<bool> {
    stage(SdkStage::Engine)?;
    let isolated = native::loaded_images()
        .iter()
        .any(|path| std::path::Path::new(path) == engine_path);
    if !isolated {
        return Err("Unexpected Steam engine image".into());
    }
    stage(SdkStage::Interfaces)?;
    let get_user = symbol!(
        api,
        c"SteamAPI_SteamUser_v023",
        unsafe extern "C" fn() -> *mut c_void
    );
    let get_apps = symbol!(
        api,
        c"SteamAPI_SteamApps_v008",
        unsafe extern "C" fn() -> *mut c_void
    );
    let get_utils = symbol!(
        api,
        c"SteamAPI_SteamUtils_v010",
        unsafe extern "C" fn() -> *mut c_void
    );
    let get_stats = symbol!(
        api,
        c"SteamAPI_SteamUserStats_v012",
        unsafe extern "C" fn() -> *mut c_void
    );
    let (user, apps, utils, stats) = (
        call!(get_user()),
        call!(get_apps()),
        call!(get_utils()),
        call!(get_stats()),
    );
    if [user, apps, utils, stats]
        .iter()
        .any(|value| value.is_null())
    {
        return Err("Missing Steamworks interface".into());
    }
    stage(SdkStage::Account)?;
    let online = symbol!(
        api,
        c"SteamAPI_ISteamUser_BLoggedOn",
        unsafe extern "C" fn(*mut c_void) -> bool
    );
    let id = symbol!(
        api,
        c"SteamAPI_ISteamUser_GetSteamID",
        unsafe extern "C" fn(*mut c_void) -> u64
    );
    let subscribed = symbol!(
        api,
        c"SteamAPI_ISteamApps_BIsSubscribedApp",
        unsafe extern "C" fn(*mut c_void, u32) -> bool
    );
    let current = symbol!(
        api,
        c"SteamAPI_ISteamUtils_GetAppID",
        unsafe extern "C" fn(*mut c_void) -> u32
    );
    if !call!(online(user))
        || call!(id(user)) != expected
        || !call!(subscribed(apps, app))
        || call!(current(utils)) != app
    {
        return Err("Steamworks account/license verification failed".into());
    }
    stage(SdkStage::AsynchronousRequest)?;
    let request = symbol!(
        api,
        c"SteamAPI_ISteamUserStats_GetNumberOfCurrentPlayers",
        unsafe extern "C" fn(*mut c_void) -> u64
    );
    let completed = symbol!(
        api,
        c"SteamAPI_ISteamUtils_IsAPICallCompleted",
        unsafe extern "C" fn(*mut c_void, u64, *mut bool) -> bool
    );
    let get_result = symbol!(
        api,
        c"SteamAPI_ISteamUtils_GetAPICallResult",
        unsafe extern "C" fn(*mut c_void, u64, *mut c_void, i32, i32, *mut bool) -> bool
    );
    let callbacks = symbol!(api, c"SteamAPI_RunCallbacks", unsafe extern "C" fn());
    let handle = call!(request(stats));
    let started = Instant::now();
    while handle != 0 && started.elapsed() < Duration::from_secs(8) {
        call!(callbacks());
        let mut failed = false;
        if call!(completed(utils, handle, &mut failed)) {
            let mut players = Players::default();
            let received = call!(get_result(
                utils,
                handle,
                (&mut players as *mut Players).cast(),
                8,
                1107,
                &mut failed
            ));
            return Ok(received && !failed && players.success == 1 && players.count >= 0);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The player-count endpoint is optional; verified account/license are required.
    Ok(false)
}
