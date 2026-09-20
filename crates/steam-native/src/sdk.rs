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
mod api;
use api::{Api, Interface};
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

fn verify_storage_files(api: &Api, files: &[(String, std::path::PathBuf)]) -> Result<()> {
    use std::ffi::CString;
    let storage = api.interface(Interface::Storage)?;
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

fn storage_quota(api: &Api) -> Result<(u64, u64)> {
    let storage = api.interface(Interface::Storage)?;
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
    extra: impl FnOnce(&Api) -> Result<()>,
) -> Result<bool> {
    crate::pinned_client(engine_path)?;
    let mut identity = [0; 8];
    std::io::stdin().read_exact(&mut identity)?;
    let expected = u64::from_le_bytes(identity);
    if expected == 0 || app == 0 {
        return Err("Invalid SDK verification request".into());
    }
    stage(SdkStage::Library)?;
    let library = Library::open(api_path)?;
    stage(SdkStage::Initialize)?;
    let api = Api::initialize(library, api_path)?;
    verify_interfaces(&api, engine_path, expected, app, stage)
        .and_then(|asynchronous| extra(&api).map(|()| asynchronous))
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
        let storage = api.interface(Interface::Storage)?;
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
        crate::events::emit(
            output,
            &steam_runtime::protocol::Event::StorageFailure {
                reason: error.to_string(),
            },
        )?;
        return Err(error);
    }
    crate::events::emit(
        output,
        &steam_runtime::protocol::Event::StorageVerified {
            total: quota.0,
            available: quota.1,
            readback,
        },
    )
}
fn verify_interfaces(
    api: &Api,
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
    let user = api.interface(Interface::User)?;
    let apps = api.interface(Interface::Apps)?;
    let utils = api.interface(Interface::Utils)?;
    let stats = api.interface(Interface::Stats)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    const ACCOUNT: u64 = 76561198000000001;
    const APP: u32 = 588650;

    struct Fixture(std::path::PathBuf, bool);
    impl Fixture {
        fn new(defines: &[&str]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "forge-sdk-fixture-{}-{}.dylib",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let output = std::process::Command::new("/usr/bin/clang")
                .args(["-dynamiclib", "-Wall", "-Werror"])
                .args(defines.iter().map(|define| format!("-D{define}")))
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/steam_api.c"
                ))
                .arg("-o")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self(path.canonicalize().unwrap(), defines.contains(&"LEGACY"))
        }
        fn initialize(&self) -> Result<Api> {
            let library = Library::open(&self.0)?;
            if self.1 {
                Api::initialize_legacy_fixture(library)
            } else {
                Api::initialize(library, &self.0)
            }
        }
        fn verify(&self, api: &Api, account: u64, app: u32) -> Result<bool> {
            verify_interfaces(api, &self.0, account, app, &mut |_| Ok(()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn legacy_sdk_verifies_identity_without_modern_interface_exports() -> Result<()> {
        let fixture = Fixture::new(&["LEGACY"]);
        assert!(fixture.verify(&fixture.initialize()?, ACCOUNT, APP)?);
        Ok(())
    }

    #[test]
    fn unrecognized_legacy_library_is_rejected_before_initialization() -> Result<()> {
        let fixture = Fixture::new(&["LEGACY"]);
        assert!(Api::initialize(Library::open(&fixture.0)?, &fixture.0).is_err());
        let library = Library::open(&fixture.0)?;
        let client = symbol!(
            &library,
            c"SteamClient",
            unsafe extern "C" fn() -> *mut c_void
        );
        assert!(call!(client()).is_null());
        Ok(())
    }

    #[test]
    fn modern_sdk_keeps_identity_license_and_callback_verification() -> Result<()> {
        let fixture = Fixture::new(&[]);
        let api = fixture.initialize()?;
        assert!(fixture.verify(&api, ACCOUNT, APP)?);
        assert!(fixture.verify(&api, ACCOUNT + 1, APP).is_err());
        assert!(fixture.verify(&api, ACCOUNT, APP + 1).is_err());
        Ok(())
    }

    #[test]
    fn legacy_sdk_rejects_wrong_identity_and_license() -> Result<()> {
        let fixture = Fixture::new(&["LEGACY"]);
        let api = fixture.initialize()?;
        assert!(fixture.verify(&api, ACCOUNT + 1, APP).is_err());
        assert!(fixture.verify(&api, ACCOUNT, APP + 1).is_err());
        Ok(())
    }

    #[test]
    fn missing_required_interfaces_never_pass_verification() -> Result<()> {
        for defines in [vec!["MISSING_USER"], vec!["LEGACY", "MISSING_USER"]] {
            let fixture = Fixture::new(&defines);
            let api = fixture.initialize()?;
            assert!(fixture.verify(&api, ACCOUNT, APP).is_err());
        }
        Ok(())
    }

    #[test]
    fn failed_or_missing_initialization_never_falls_back_to_success() {
        for defines in [
            vec!["MISSING_INIT"],
            vec!["LEGACY", "MISSING_INIT"],
            vec!["MODERN_INIT_FAIL"],
            vec!["LEGACY", "LEGACY_INIT_FAIL"],
        ] {
            let fixture = Fixture::new(&defines);
            assert!(fixture.initialize().is_err());
        }
    }

    #[test]
    fn legacy_and_modern_sdk_read_the_projected_storage_bytes() -> Result<()> {
        for defines in [vec![], vec!["LEGACY"]] {
            let fixture = Fixture::new(&defines);
            let api = fixture.initialize()?;
            assert_eq!(storage_quota(&api)?, (1000000, 900000));
            let save = fixture.0.with_extension("save");
            std::fs::write(&save, b"save")?;
            let result = verify_storage_files(&api, &[("fixture.dat".into(), save.clone())]);
            std::fs::remove_file(save)?;
            result?;
        }
        Ok(())
    }

    #[test]
    fn sdk_shutdown_runs_when_verification_rejects_an_account() -> Result<()> {
        let fixture = Fixture::new(&["LEGACY"]);
        let api = fixture.initialize()?;
        assert!(fixture.verify(&api, ACCOUNT + 1, APP).is_err());
        let online = symbol!(
            &api,
            c"SteamAPI_ISteamUser_BLoggedOn",
            unsafe extern "C" fn(*mut c_void) -> bool
        );
        let user = api.interface(Interface::User)?;
        assert!(call!(online(user)));
        drop(api);
        assert!(!call!(online(user)));
        Ok(())
    }
}
