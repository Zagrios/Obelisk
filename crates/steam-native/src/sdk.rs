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
    verify_with(api_path, engine_path, app, &mut stage, |api| {
        if std::env::var_os("FORGE_STORAGE_METADATA").is_some() {
            storage_quota(api)?;
            if let Some(directory) = std::env::var_os("FORGE_STORAGE_DIRECTORY") {
                let files = steam_runtime::native_cache_files(directory.into(), app)?;
                verify_storage_files(
                    api,
                    &files
                        .iter()
                        .filter(|f| f.path.is_file())
                        .map(|f| (f.name.clone(), f.path.clone()))
                        .collect::<Vec<_>>(),
                )?;
            }
        }
        Ok(())
    })
}

fn verify_storage_files(api: &Library, files: &[(String, std::path::PathBuf)]) -> Result<()> {
    use std::ffi::CString;
    let get = symbol!(
        api,
        c"SteamAPI_SteamRemoteStorage_v016",
        unsafe extern "C" fn() -> *mut c_void
    );
    let storage = call!(get());
    if storage.is_null() {
        return Err("RemoteStorage missing".into());
    }
    let count = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_GetFileCount",
        unsafe extern "C" fn(*mut c_void) -> i32
    );
    let exists = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_FileExists",
        unsafe extern "C" fn(*mut c_void, *const c_char) -> bool
    );
    let read = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_FileRead",
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, i32) -> i32
    );
    let callbacks = symbol!(api, c"SteamAPI_RunCallbacks", unsafe extern "C" fn());
    let start = Instant::now();
    loop {
        call!(callbacks());
        let mut matches = call!(count(storage)) == files.len() as i32;
        if matches {
            for (name, path) in files {
                if std::fs::metadata(path)?.len() > 100 * 1024 * 1024 {
                    return Err("SDK verification file too large".into());
                }
                let expected = std::fs::read(path)?;
                let name = CString::new(name.as_bytes())?;
                let mut actual = vec![0; expected.len()];
                matches &= call!(exists(storage, name.as_ptr()))
                    && call!(read(
                        storage,
                        name.as_ptr(),
                        actual.as_mut_ptr().cast(),
                        actual.len() as i32
                    )) == expected.len() as i32
                    && actual == expected;
            }
        }
        if matches {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err("Native SDK cache not ready or differs from projection".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn storage_quota(api: &Library) -> Result<(u64, u64)> {
    let get = symbol!(
        api,
        c"SteamAPI_SteamRemoteStorage_v016",
        unsafe extern "C" fn() -> *mut c_void
    );
    let storage = call!(get());
    if storage.is_null() {
        return Err("RemoteStorage missing".into());
    }
    let get_quota = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_GetQuota",
        unsafe extern "C" fn(*mut c_void, *mut u64, *mut u64) -> bool
    );
    let callbacks = symbol!(api, c"SteamAPI_RunCallbacks", unsafe extern "C" fn());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        call!(callbacks());
        let mut quota = (0, 0);
        if call!(get_quota(storage, &mut quota.0, &mut quota.1)) && quota.0 > 0 {
            return Ok(quota);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("Native Cloud quota unavailable".into())
}

fn verify_with(
    api_path: &std::path::Path,
    engine_path: &std::path::Path,
    app: u32,
    stage: &mut impl FnMut(SdkStage) -> Result<()>,
    extra: impl FnOnce(&Library) -> Result<()>,
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
    let result = verify_interfaces(&api, engine_path, expected, app, stage)
        .and_then(|asynchronous| extra(&api).map(|()| asynchronous));
    let shutdown = symbol!(api, c"SteamAPI_Shutdown", unsafe extern "C" fn());
    call!(shutdown());
    result
}

#[cfg(debug_assertions)]
pub fn storage_probe(
    api: &std::path::Path,
    engine: &std::path::Path,
    app: u32,
    readback: bool,
    output: &mut std::fs::File,
) -> Result<()> {
    let mut quota = (0, 0);
    let verification = verify_with(api, engine, app, &mut |_| Ok(()), |api| {
        let get = symbol!(
            api,
            c"SteamAPI_SteamRemoteStorage_v016",
            unsafe extern "C" fn() -> *mut c_void
        );
        let storage = call!(get());
        if storage.is_null() {
            return Err("RemoteStorage missing".into());
        }
        let get_quota = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_GetQuota",
            unsafe extern "C" fn(*mut c_void, *mut u64, *mut u64) -> bool
        );
        let callbacks = symbol!(api, c"SteamAPI_RunCallbacks", unsafe extern "C" fn());
        let deadline = Instant::now();
        while deadline.elapsed() < Duration::from_secs(10) {
            call!(callbacks());
            if call!(get_quota(storage, &mut quota.0, &mut quota.1)) && quota.0 > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if quota.0 == 0 {
            return Err("Native Cloud quota unavailable".into());
        }
        let count = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_GetFileCount",
            unsafe extern "C" fn(*mut c_void) -> i32
        );
        let write = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_FileWrite",
            unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void, i32) -> bool
        );
        let read = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_FileRead",
            unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, i32) -> i32
        );
        let delete = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_FileDelete",
            unsafe extern "C" fn(*mut c_void, *const c_char) -> bool
        );
        let name = c"forge-sdk-persistence.dat";
        let payload = b"Forge authenticated SDK persistence fixture\n";
        if !readback {
            if call!(count(storage)) != 0 {
                return Err("Probe native namespace not empty".into());
            }
            if !call!(write(
                storage,
                name.as_ptr(),
                payload.as_ptr().cast(),
                payload.len() as i32
            )) {
                return Err("SDK file write failed".into());
            }
        }
        let directory =
            std::env::var_os("FORGE_STORAGE_DIRECTORY").ok_or("Probe cache directory missing")?;
        verify_storage_files(
            api,
            &[(
                name.to_string_lossy().into_owned(),
                std::path::PathBuf::from(directory)
                    .join("remote")
                    .join(name.to_string_lossy().as_ref()),
            )],
        )?;
        let filename = symbol!(
            api,
            c"SteamAPI_ISteamRemoteStorage_GetFileNameAndSize",
            unsafe extern "C" fn(*mut c_void, i32, *mut i32) -> *const c_char
        );
        let mut size = 0;
        let enumerated = call!(filename(storage, 0, &mut size));
        if enumerated.is_null() || size != payload.len() as i32 {
            return Err("SDK enumeration failed".into());
        }
        // SAFETY: Steam returns a NUL-terminated name valid until the next call.
        if unsafe { std::ffi::CStr::from_ptr(enumerated) } != name {
            return Err("SDK enumeration name mismatch".into());
        }
        let mut bytes = vec![0; payload.len()];
        if call!(count(storage)) != 1
            || call!(read(
                storage,
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len() as i32
            )) != payload.len() as i32
            || bytes != payload
        {
            return Err("SDK persisted readback failed".into());
        }
        if readback && (!call!(delete(storage, name.as_ptr())) || call!(count(storage)) != 0) {
            return Err("SDK file deletion failed".into());
        }
        Ok(())
    });
    if let Err(error) = verification {
        crate::host::emit(
            output,
            &steam_runtime::protocol::Event::StorageFailure {
                reason: error.to_string(),
            },
        )?;
        return Err(error);
    }
    crate::host::emit(
        output,
        &steam_runtime::protocol::Event::StorageVerified {
            total: quota.0,
            available: quota.1,
            readback,
        },
    )
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
