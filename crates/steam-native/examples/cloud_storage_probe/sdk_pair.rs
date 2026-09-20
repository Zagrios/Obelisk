//! Two-process offline SDK fixture. No production authentication path is changed.
use super::{load_cache, method, read_fixture, report};
use forge_steam_native::{Result, abi, mach, native, symbol};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    fs::File,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const APP: u32 = 480;
const IDENTITY: u64 = 0x0110_0001_0000_0001;
const HOST_NAME: &CStr = c"forge-sdk-host.dat";
const HOST_BYTES: &[u8] = b"Forge offline SDK host fixture v1\n";
const CONSUMER_NAME: &CStr = c"forge-sdk-consumer.dat";
const CONSUMER_BYTES: &[u8] = b"Forge offline SDK consumer fixture v1\n";
const REPLY_NAME: &CStr = c"forge-sdk-reply.dat";
const REPLY_BYTES: &[u8] = b"Forge offline SDK host reply after observing consumer write\n";

fn fixture_service() -> Result<CString> {
    if std::env::var_os("FORGE_CLOUD_PROBE_IDENTITY").is_none() {
        return Err("SDK pair requires the disconnected test identity".into());
    }
    let service = std::env::var("FORGE_PRIVATE_STEAM_DISCOVERY")?;
    if !service.starts_with("com.zagrios.forge.discovery.") || service.len() > 120 {
        return Err("Invalid fixture discovery service".into());
    }
    Ok(CString::new(service)?)
}

pub(super) fn serve(
    output: &mut File,
    storage: *mut c_void,
    base: usize,
    scratch: &Path,
    engine: &abi::Engine,
    user: i32,
    pipe: i32,
) -> Result<()> {
    let service = fixture_service()?;
    if engine.logged_on(user, pipe) || engine.user(user, pipe)?.steam_id()? != IDENTITY {
        return Err("SDK host requires the unauthenticated fixture identity".into());
    }
    let executable = std::env::current_exe()?.canonicalize()?;
    if executable.parent() != Some(scratch.join("runtime").as_path())
        || !mach::verify_host(std::process::id() as i32, &executable)
    {
        return Err("SDK fixture host must be beside its pinned runtime".into());
    }
    let path = executable.as_os_str().as_bytes();
    if path.len() >= 512 {
        return Err("SDK fixture host path exceeds discovery limit".into());
    }
    load_cache(output, storage, base, engine, pipe)?;
    private_write(storage, base, HOST_NAME, HOST_BYTES)?;
    read_fixture(output, storage, base, HOST_NAME, HOST_BYTES)?;
    let port = mach::ReceivePort::new()?;
    let _registration = port.register(service)?;
    let stopped = Arc::new(AtomicBool::new(false));
    let input_closed = stopped.clone();
    std::thread::spawn(move || {
        let _ = std::io::stdin().read(&mut [0_u8]);
        input_closed.store(true, Ordering::Release);
    });
    report(
        output,
        serde_json::json!({"event":"sdk_host_ready", "host_pid":std::process::id(),
            "app_id":APP, "authenticated":false, "lifetime_seconds":25}),
    )?;
    let started = Instant::now();
    let mut observed_write = false;
    let mut paths = 0_u32;
    let mut callback_count = 0_u32;
    while !stopped.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(25) {
        for _ in 0..100 {
            if engine.callback(pipe).is_none() {
                break;
            }
            callback_count = callback_count.saturating_add(1);
        }
        if !observed_write && private_read_matches(storage, base, CONSUMER_NAME, CONSUMER_BYTES)? {
            observed_write = true;
            private_write(storage, base, REPLY_NAME, REPLY_BYTES)?;
            read_fixture(output, storage, base, REPLY_NAME, REPLY_BYTES)?;
            report(
                output,
                serde_json::json!({"event":"sdk_host_observed_consumer_write",
                    "matches_fixture":true, "reply_written":true}),
            )?;
        }
        let mut request = mach::Message::default();
        let status = mach::receive(
            &mut request,
            port.0,
            std::mem::size_of::<mach::Message>() as u32,
            50,
        );
        if [mach::RECEIVE_TIMED_OUT, mach::RECEIVE_INTERRUPTED].contains(&status) {
            continue;
        }
        if status != 0 {
            return Err("SDK fixture discovery receive failed".into());
        }
        if !mach::valid_request(&request) {
            mach::destroy(&mut request);
            continue;
        }
        // This is the validated message layout used by host::serve. Only the
        // fixture's real PID/path is supplied; SteamAPI results remain native.
        let mut reply = mach::Message::default();
        reply.header.bits = if request.header.bits & 0xff == 18 {
            18
        } else {
            17
        };
        reply.header.remote = request.header.remote;
        reply.header.id = 104;
        reply.opcode = request.opcode;
        if request.opcode == 100001 {
            reply.header.size = 32;
            reply.value = 104;
        } else {
            reply.header.size = 544;
            reply.value = std::process::id();
            reply.path[..path.len()].copy_from_slice(path);
            paths = paths.saturating_add(1);
            report(
                output,
                serde_json::json!({"event":"sdk_host_discovery", "paths":paths}),
            )?;
        }
        if mach::send(&mut reply) != 0 {
            mach::destroy(&mut reply);
        }
    }
    report(
        output,
        serde_json::json!({"event":"sdk_host_complete", "consumer_write_seen":observed_write,
            "discovery_paths":paths, "callbacks":callback_count,
            "stopped_by_stdin":stopped.load(Ordering::Acquire)}),
    )?;
    if !stopped.load(Ordering::Acquire) {
        return Err("SDK fixture host lifetime deadline reached".into());
    }
    if !observed_write || paths == 0 {
        return Err("SDK fixture consumer exchange was not observed".into());
    }
    Ok(())
}

fn private_write(storage: *mut c_void, base: usize, name: &CStr, payload: &[u8]) -> Result<()> {
    let address = method(storage, base, 0, 0x8fb410)?;
    // SAFETY: pinned private FileWrite layout established by the public adapter,
    // matching the existing offline fixture. Payload and filename outlive call.
    let result = unsafe {
        let write: unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            *const c_char,
            *const u8,
            u32,
        ) -> u32 = std::mem::transmute(address);
        write(
            storage,
            APP,
            0,
            name.as_ptr(),
            payload.as_ptr(),
            payload.len() as u32,
        )
    };
    if result != 1 {
        return Err(format!("SDK host fixture write returned {result}").into());
    }
    Ok(())
}

fn private_read_matches(
    storage: *mut c_void,
    base: usize,
    name: &CStr,
    payload: &[u8],
) -> Result<bool> {
    let address = method(storage, base, 5, 0x8fbd48)?;
    let mut bytes = [0_u8; 256];
    // SAFETY: pinned private FileRead layout from read_fixture; initialized
    // bounded buffer, valid static name, result checked before any slice access.
    let count = unsafe {
        let read: unsafe extern "C" fn(*mut c_void, u32, u32, *const c_char, *mut u8, u32) -> i32 =
            std::mem::transmute(address);
        read(
            storage,
            APP,
            0,
            name.as_ptr(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
        )
    };
    check_bytes(count, &bytes, payload)
}

fn check_bytes(count: i32, bytes: &[u8], expected: &[u8]) -> Result<bool> {
    if count == 0 {
        return Ok(false);
    }
    if count == expected.len() as i32 && bytes.get(..count as usize) == Some(expected) {
        return Ok(true);
    }
    Err(format!("SDK fixture content mismatch, returned {count} bytes").into())
}

pub(super) fn consume(output: &mut File, engine_path: &Path, scratch: &Path) -> Result<()> {
    let _service = fixture_service()?;
    let adapter = scratch.join("runtime/libforge_steam_discovery_adapter.dylib");
    let discovery_loaded = native::loaded_images()
        .iter()
        .any(|image| Path::new(image) == adapter);
    report(
        output,
        serde_json::json!({"event":"sdk_discovery_adapter", "loaded":discovery_loaded}),
    )?;
    if !discovery_loaded {
        return Err("SDK fixture discovery adapter was not loaded".into());
    }
    let api_path = std::path::PathBuf::from(
        std::env::var_os("FORGE_CLOUD_PROBE_SDK").ok_or("SDK fixture library path missing")?,
    );
    if api_path != scratch.join("runtime/libsteam_api.dylib")
        || api_path.canonicalize()? != api_path
    {
        return Err("SDK fixture library must be copied inside the disposable runtime".into());
    }
    let api = native::Library::open(&api_path)?;
    let init = symbol!(
        api,
        c"SteamAPI_InitFlat",
        unsafe extern "C" fn(*mut c_char) -> i32
    );
    let shutdown = symbol!(api, c"SteamAPI_Shutdown", unsafe extern "C" fn());
    let mut error = [0_i8; 1024];
    // SAFETY: original SDK flat C signature and full 1024-byte error buffer,
    // matching sdk::verify. The separate host alone owns the global user.
    let result = unsafe { init(error.as_mut_ptr()) };
    let error_bytes: Vec<u8> = error
        .iter()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    report(
        output,
        serde_json::json!({"event":"sdk_init", "result":result,
        "detail":String::from_utf8_lossy(&error_bytes), "consumer_pid":std::process::id()}),
    )?;
    if result != 0 {
        return Err("Genuine offline SteamAPI initialization failed".into());
    }
    let result = consume_initialized(output, &api, engine_path);
    // SAFETY: the original SDK initialized successfully in this process.
    unsafe { shutdown() };
    result
}

fn consume_initialized(output: &mut File, api: &native::Library, engine_path: &Path) -> Result<()> {
    let images = native::loaded_images();
    let engine_images: Vec<_> = images
        .iter()
        .filter(|path| {
            Path::new(path)
                .file_name()
                .is_some_and(|name| name == "steamclient.dylib")
        })
        .collect();
    let isolated = engine_images.len() == 1 && Path::new(engine_images[0]) == engine_path;
    report(
        output,
        serde_json::json!({"event":"sdk_engine", "matches_pinned_path":isolated}),
    )?;
    if !isolated {
        return Err("SDK consumer loaded an unexpected engine image".into());
    }
    let get_utils = symbol!(
        api,
        c"SteamAPI_SteamUtils_v010",
        unsafe extern "C" fn() -> *mut c_void
    );
    let get_user = symbol!(
        api,
        c"SteamAPI_SteamUser_v023",
        unsafe extern "C" fn() -> *mut c_void
    );
    let get_storage = symbol!(
        api,
        c"SteamAPI_SteamRemoteStorage_v016",
        unsafe extern "C" fn() -> *mut c_void
    );
    // SAFETY: versioned original Steamworks flat accessors, no arguments.
    let (utils, user, storage) = unsafe { (get_utils(), get_user(), get_storage()) };
    if [utils, user, storage]
        .iter()
        .any(|interface| interface.is_null())
    {
        return Err("SDK consumer interface unavailable".into());
    }
    let get_app = symbol!(
        api,
        c"SteamAPI_ISteamUtils_GetAppID",
        unsafe extern "C" fn(*mut c_void) -> u32
    );
    let get_id = symbol!(
        api,
        c"SteamAPI_ISteamUser_GetSteamID",
        unsafe extern "C" fn(*mut c_void) -> u64
    );
    let logged_on = symbol!(
        api,
        c"SteamAPI_ISteamUser_BLoggedOn",
        unsafe extern "C" fn(*mut c_void) -> bool
    );
    // SAFETY: published flat signatures on their matching non-null interfaces.
    let (app, identity, online) = unsafe { (get_app(utils), get_id(user), logged_on(user)) };
    report(
        output,
        serde_json::json!({"event":"sdk_context", "app_id":app,
        "matches_fixture_identity":identity == IDENTITY, "logged_on":online}),
    )?;
    if app != APP || identity != IDENTITY || online {
        return Err("SDK consumer did not inherit the offline fixture context".into());
    }
    let write = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_FileWrite",
        unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, i32) -> bool
    );
    let read = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_FileRead",
        unsafe extern "C" fn(*mut c_void, *const c_char, *mut u8, i32) -> i32
    );
    let exists = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_FileExists",
        unsafe extern "C" fn(*mut c_void, *const c_char) -> bool
    );
    let count = symbol!(
        api,
        c"SteamAPI_ISteamRemoteStorage_GetFileCount",
        unsafe extern "C" fn(*mut c_void) -> i32
    );
    let callbacks = symbol!(api, c"SteamAPI_RunCallbacks", unsafe extern "C" fn());
    let read_matches = |name: &CStr, expected: &[u8]| -> Result<bool> {
        let mut bytes = [0_u8; 256];
        // SAFETY: public FileRead C signature, matching interface and initialized
        // writable buffer. check_bytes validates the returned count and contents.
        let size = unsafe {
            read(
                storage,
                name.as_ptr(),
                bytes.as_mut_ptr(),
                bytes.len() as i32,
            )
        };
        check_bytes(size, &bytes, expected)
    };
    let host_matches = read_matches(HOST_NAME, HOST_BYTES)?;
    report(
        output,
        serde_json::json!({"event":"sdk_host_fixture_read", "matches_fixture":host_matches}),
    )?;
    if !host_matches {
        return Err("SDK consumer cannot read the host fixture".into());
    }
    // SAFETY: public FileWrite C signature; static payload/name and checked size.
    let written = unsafe {
        write(
            storage,
            CONSUMER_NAME.as_ptr(),
            CONSUMER_BYTES.as_ptr(),
            CONSUMER_BYTES.len() as i32,
        )
    };
    report(
        output,
        serde_json::json!({"event":"sdk_consumer_write", "accepted":written}),
    )?;
    if !written || !read_matches(CONSUMER_NAME, CONSUMER_BYTES)? {
        return Err("SDK consumer write/read did not match the fixture".into());
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        // SAFETY: original SDK callback pump, initialized on this thread.
        unsafe { callbacks() };
        if read_matches(REPLY_NAME, REPLY_BYTES)? {
            // SAFETY: public RemoteStorage methods on the validated interface.
            let (file_count, all_exist) = unsafe {
                (
                    count(storage),
                    [HOST_NAME, CONSUMER_NAME, REPLY_NAME]
                        .iter()
                        .all(|name| exists(storage, name.as_ptr())),
                )
            };
            report(
                output,
                serde_json::json!({"event":"sdk_consumer_complete",
                "reply_matches_fixture":true, "file_count":file_count,
                "all_files_exist":all_exist, "consumer_pid":std::process::id()}),
            )?;
            if file_count != 3 || !all_exist {
                return Err("SDK consumer enumeration did not match the fixture".into());
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("SDK consumer did not receive the host reply before deadline".into())
}
