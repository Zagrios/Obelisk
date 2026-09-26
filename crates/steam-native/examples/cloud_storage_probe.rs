//! Offline Cloud interface probe. Run only through the disposable-data runner.
//! This example is not included in SailOver's application build.
use sailover_steam_native::{Result, abi, host, native, pinned_client, symbol};
use std::{ffi::c_void, io::Write, path::Path};

#[path = "cloud_storage_probe/live_storage.rs"]
mod live_storage;
#[path = "cloud_storage_probe/sync_controls.rs"]
mod sync_controls;

fn main() {
    let result = run();
    // SAFETY: the process owns the loaded Valve image; its private unload ABI is
    // unknown. The probe flushes its report before exiting without destructors.
    unsafe { libc::_exit(i32::from(result.is_err())) }
}

fn run() -> Result<()> {
    native::disable_core_dumps()?;
    let mut output = host::event_pipe()?;
    let result = probe(&mut output);
    if let Err(error) = &result {
        report(
            &mut output,
            serde_json::json!({"event": "probe_error", "detail": error.to_string()}),
        )?;
    }
    result
}

fn probe(output: &mut std::fs::File) -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3
        || ![
            "inventory",
            "storage-inventory",
            "local-write",
            "local-read",
            "project-write",
            "project-read",
            "project-rename",
            "project-read-renamed",
            "live-mutate",
            "live-read",
            "sync-download",
            "sync-upload",
            "sync-cancel",
        ]
        .contains(&args[1].as_str())
    {
        return Err("Expected a probe mode and a verified runtime path".into());
    }
    let path = Path::new(&args[2]);
    let scratch = std::path::PathBuf::from(std::env::var("SAILOVER_CLOUD_PROBE_ROOT")?);
    if scratch.parent() != Some(Path::new("/private/tmp"))
        || !scratch
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("sailover-cloud-probe-"))
        || path != scratch.join("runtime/steamclient.dylib")
        || std::env::var_os("HOME").as_deref() != Some(scratch.join("home").as_os_str())
    {
        return Err("Use the disposable-data runner".into());
    }
    pinned_client(path)?;
    let library = native::Library::open(path)?;
    let factory = symbol!(
        library,
        c"CreateInterface",
        unsafe extern "C" fn(*const std::ffi::c_char, *mut i32) -> *mut c_void
    );
    // SAFETY: the factory is an exported C function with a static version name
    // and an initialized writable result scalar. No account is created here.
    let (object, base) = unsafe {
        let mut result = -1;
        let object = factory(c"SteamClient020".as_ptr(), &mut result);
        if result != 0 || object.is_null() {
            return Err("Public client interface unavailable".into());
        }
        let mut info: libc::Dl_info = std::mem::zeroed();
        if libc::dladdr(factory as *const c_void, &mut info) == 0 {
            return Err("Cannot identify the pinned image".into());
        }
        (object, info.dli_fbase as usize)
    };
    report(
        output,
        serde_json::json!({
            "event": "client_interface_inventory", "interface": "SteamClient020",
            "authenticated": false, "methods": inventory(object, base, 19)?,
        }),
    )?;
    if args[1] != "inventory" {
        let engine = abi::Engine::open(path)?;
        let (user, pipe) = engine.global();
        if std::env::var_os("SAILOVER_CLOUD_PROBE_IDENTITY").is_some() {
            // Disposable account component 1, no credential or external network.
            // This is not authentication or a production identity substitution.
            let requested = engine.user(user, pipe)?.log_on(0x0110_0001_0000_0001)?;
            report(
                output,
                serde_json::json!({"event": "fixture_identity_requested", "result": requested}),
            )?;
        }
        report(
            output,
            serde_json::json!({
                "event": "global_user", "user": user, "pipe": pipe,
                "connected": engine.connected(user, pipe), "logged_on": engine.logged_on(user, pipe),
            }),
        )?;
        if user == 0 || pipe == 0 || engine.logged_on(user, pipe) {
            return Err("Expected an unauthenticated disposable user".into());
        }
        let identity = engine.user(user, pipe)?.steam_id()?;
        report(
            output,
            serde_json::json!({"event": "identity", "is_zero": identity == 0,
            "account_id_is_zero": identity as u32 == 0,
            "instance": (identity >> 32) & 0xfffff, "universe": identity >> 56,
            "account_type": (identity >> 52) & 0xf}),
        )?;
        // SAFETY: the pinned factory accepts this static engine version name.
        let engine_object = unsafe {
            let mut result = -1;
            let object = factory(c"CLIENTENGINE_INTERFACE_VERSION005".as_ptr(), &mut result);
            if result != 0 || object.is_null() {
                return Err("Private engine unavailable".into());
            }
            object
        };
        let address = method(engine_object, base, 24, 0xa03cac)?;
        // SAFETY: pinned engine slot 24 validates user/pipe, then returns record
        // member +0x68. Disassembly establishes this,user32,pipe32 -> pointer.
        // See docs/steam-cloud-runtime-controls.md. No storage method is called.
        let storage = unsafe {
            let get: unsafe extern "C" fn(*mut c_void, i32, i32) -> *mut c_void =
                std::mem::transmute(address);
            get(engine_object, user, pipe)
        };
        if storage.is_null() {
            return Err("Private RemoteStorage unavailable".into());
        }
        if args[1].starts_with("live-") {
            let address = method(engine_object, base, 14, 0xa04d74)?;
            // SAFETY: pinned engine slot 14 validates a single pipe handle and
            // returns its Utils object. Public factory calls its slot 19.
            let utils = unsafe {
                let get: unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void =
                    std::mem::transmute(address);
                get(engine_object, pipe)
            };
            if utils.is_null() {
                return Err("Private Utils unavailable".into());
            }
            report(
                output,
                serde_json::json!({"event":"utils_inventory", "methods":inventory(utils, base, 20)?}),
            )?;
            live_storage::prepare_pipe_app(output, utils, base)?;
        }
        report(
            output,
            serde_json::json!({
                "event": "storage_interface_inventory", "interface": "IClientRemoteStorage",
                "methods": inventory(storage, base, 89)?,
            }),
        )?;
        let address = method(object, base, 17, 0x7c9918)?;
        // SAFETY: published ISteamClient::GetISteamRemoteStorage signature;
        // pinned wrapper selects "RemoteStorage", with the version below
        // present in this image. It returns null for unavailable interfaces.
        let public_storage = unsafe {
            let get: unsafe extern "C" fn(
                *mut c_void,
                i32,
                i32,
                *const std::ffi::c_char,
            ) -> *mut c_void = std::mem::transmute(address);
            get(
                object,
                user,
                pipe,
                c"STEAMREMOTESTORAGE_INTERFACE_VERSION016".as_ptr(),
            )
        };
        if public_storage.is_null() {
            return Err("Public RemoteStorage unavailable".into());
        }
        report(
            output,
            serde_json::json!({
                "event": "storage_interface_inventory", "interface": "ISteamRemoteStorage016",
                "methods": inventory(public_storage, base, 23)?,
            }),
        )?;
        let result = if args[1].starts_with("sync-") {
            sync_controls::run(output, storage, base, pipe, &library, &args[1])
        } else if args[1].starts_with("live-") {
            let storage = live_storage::Storage::new(storage, public_storage, base);
            storage.run(output, &args[1], &scratch, &engine, pipe)
        } else if args[1].starts_with("local-") || args[1].starts_with("project-") {
            local_storage(output, storage, base, &args[1], &scratch, &engine, pipe)
        } else {
            Ok(())
        };
        engine.release(user, pipe);
        result?;
    }
    Ok(())
}

fn local_storage(
    output: &mut std::fs::File,
    storage: *mut c_void,
    base: usize,
    mode: &str,
    scratch: &Path,
    engine: &abi::Engine,
    pipe: i32,
) -> Result<()> {
    const APP: u32 = 480;
    const ROOT: u32 = 0;
    const PAYLOAD: &[u8] = b"SailOver offline RemoteStorage fixture v1\n";
    let account_component = if std::env::var_os("SAILOVER_CLOUD_PROBE_IDENTITY").is_some() {
        "1"
    } else {
        "0"
    };
    let mut name = if mode == "project-read-renamed" {
        c"sailover-offline-probe-renamed.dat"
    } else {
        c"sailover-offline-probe.dat"
    };
    let fixture_path = scratch.join(format!(
        "home/Library/Application Support/Steam/userdata/{account_component}/480/remote/{}",
        name.to_str()?
    ));
    let address = method(storage, base, 20, 0x8fd710)?;
    let (mut quota, mut available) = (0_u64, 0_u64);
    // SAFETY: the pinned proxy takes AppID and two writable eight-byte outputs.
    let quota_known = unsafe {
        let call: unsafe extern "C" fn(*mut c_void, u32, *mut u64, *mut u64) -> bool =
            std::mem::transmute(address);
        call(storage, APP, &mut quota, &mut available)
    };
    report(
        output,
        serde_json::json!({"event": "quota", "known": quota_known,
            "total": quota_known.then_some(quota), "available": quota_known.then_some(available)}),
    )?;
    let address = method(storage, base, 73, 0x903118)?;
    // SAFETY: pinned read-only sync-state proxy takes AppID and returns i32.
    let state = unsafe {
        let call: unsafe extern "C" fn(*mut c_void, u32) -> i32 = std::mem::transmute(address);
        call(storage, APP)
    };
    report(
        output,
        serde_json::json!({"event": "sync_state", "value": state}),
    )?;
    let resolve = method(storage, base, 67, 0x90278c)?;
    let mut path = [0_u8; 4096];
    // SAFETY: the pinned proxy serializes AppID/root/name/buffer size and copies
    // the returned path into the supplied buffer. All buffers outlive the call.
    let resolved = unsafe {
        let call: unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            *const std::ffi::c_char,
            *mut u8,
            u32,
        ) -> bool = std::mem::transmute(resolve);
        call(
            storage,
            APP,
            ROOT,
            name.as_ptr(),
            path.as_mut_ptr(),
            path.len() as u32,
        )
    };
    let length = path
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("Unterminated storage path")?;
    report(
        output,
        serde_json::json!({"event": "resolve_path", "resolved": resolved,
        "app_id": APP, "root": ROOT, "path": String::from_utf8_lossy(&path[..length])}),
    )?;
    if mode == "project-write" {
        if !resolved {
            return Err("Cannot project without a storage path".into());
        }
        let destination = std::path::PathBuf::from(std::str::from_utf8(&path[..length])?);
        let expected = &fixture_path;
        if &destination != expected {
            return Err("Refusing to project outside the disposable fixture path".into());
        }
        let parent = destination.parent().ok_or("Missing fixture parent")?;
        std::fs::create_dir_all(parent)?;
        if !parent.canonicalize()?.starts_with(scratch.canonicalize()?) {
            return Err("Unexpected storage symlink".into());
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)?;
        file.write_all(PAYLOAD)?;
        file.sync_all()?;
        report(
            output,
            serde_json::json!({"event": "sailover_projected_fixture", "bytes": PAYLOAD.len()}),
        )?;
        if std::env::var_os("SAILOVER_CLOUD_PROBE_CACHE").is_some() {
            // Test fixture only: reproduce the record emitted by this pinned
            // macOS runtime for these exact bytes. No live cache is imported.
            let timestamp = file
                .metadata()?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let cache_path = parent
                .parent()
                .ok_or("Missing cache parent")?
                .join("remotecache.vdf");
            let cache = format!(
                "\"480\"\n{{\n\"ChangeNumber\" \"0\"\n\"OSType\" \"-1\"\n\"sailover-offline-probe.dat\"\n{{\n\"root\" \"0\"\n\"size\" \"39\"\n\"localtime\" \"{timestamp}\"\n\"time\" \"{timestamp}\"\n\"remotetime\" \"0\"\n\"sha\" \"b9d228874d30ac5d864bebd5252d232a3858c866\"\n\"syncstate\" \"3\"\n\"persiststate\" \"0\"\n\"platformstosync2\" \"-1\"\n}}\n}}\n"
            );
            let mut index = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(cache_path)?;
            index.write_all(cache.as_bytes())?;
            index.sync_all()?;
            report(
                output,
                serde_json::json!({"event": "sailover_projected_cache", "fixture_records": 1}),
            )?;
        }
    }
    if mode.starts_with("project-") || mode == "local-read" {
        let direct = std::fs::read(&fixture_path)?;
        report(
            output,
            serde_json::json!({"event": "direct_file_read", "bytes": direct.len(), "matches_fixture": direct == PAYLOAD}),
        )?;
        if direct != PAYLOAD {
            return Err("Fixture missing or changed on disk".into());
        }
    }
    load_cache(output, storage, base, engine, pipe)?;
    if mode == "local-write" {
        let address = method(storage, base, 0, 0x8fb410)?;
        // SAFETY: the public FileWrite adapter proves AppID/root/name/data/size
        // order. The private return is EResult (32 bits), NOT C++ bool.
        let result = unsafe {
            let call: unsafe extern "C" fn(
                *mut c_void,
                u32,
                u32,
                *const std::ffi::c_char,
                *const u8,
                u32,
            ) -> u32 = std::mem::transmute(address);
            call(
                storage,
                APP,
                ROOT,
                name.as_ptr(),
                PAYLOAD.as_ptr(),
                PAYLOAD.len() as u32,
            )
        };
        report(
            output,
            serde_json::json!({"event": "file_write", "result": result, "accepted": result == 1}),
        )?;
        if result != 1 {
            return Err("Offline fixture write was rejected".into());
        }
        let cache = fixture_path
            .parent()
            .and_then(Path::parent)
            .ok_or("Missing cache directory")?
            .join("remotecache.vdf");
        let started = std::time::Instant::now();
        let mut checkpoint = false;
        while started.elapsed() < std::time::Duration::from_secs(5) {
            let _ = engine.callback(pipe);
            if std::fs::read_to_string(&cache)
                .is_ok_and(|text| text.contains("sailover-offline-probe.dat"))
            {
                checkpoint = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        report(
            output,
            serde_json::json!({"event": "local_cache_checkpoint", "fixture_record_present": checkpoint,
            "wait_milliseconds": started.elapsed().as_millis()}),
        )?;
        if !checkpoint {
            return Err("Native cache checkpoint not observed before deadline".into());
        }
    }
    if mode == "project-rename" {
        read_fixture(output, storage, base, name, PAYLOAD)?;
        let parent = fixture_path.parent().ok_or("Missing fixture parent")?;
        if !parent.canonicalize()?.starts_with(scratch.canonicalize()?) {
            return Err("Unexpected storage symlink".into());
        }
        let cache = parent
            .parent()
            .ok_or("Missing cache parent")?
            .join("remotecache.vdf");
        let original = std::fs::read_to_string(&cache)?;
        if !original.contains(name.to_str()?) {
            return Err("Expected initialized native cache record".into());
        }
        let renamed = c"sailover-offline-probe-renamed.dat";
        let destination = parent.join(renamed.to_str()?);
        if destination.try_exists()? {
            return Err("Renamed fixture already exists".into());
        }
        std::fs::rename(&fixture_path, &destination)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(cache)?;
        file.write_all(
            original
                .replace(name.to_str()?, renamed.to_str()?)
                .as_bytes(),
        )?;
        file.sync_all()?;
        name = renamed;
        report(
            output,
            serde_json::json!({"event": "sailover_renamed_fixture_and_cache", "direct_read_matches": std::fs::read(destination)? == PAYLOAD}),
        )?;
        load_cache(output, storage, base, engine, pipe)?;
    }
    let address = method(storage, base, 18, 0x8fd370)?;
    // SAFETY: the pinned public adapter calls private FileCount with AppID and
    // a one-byte true option, reproduced here; no buffers are involved.
    let file_count = unsafe {
        let call: unsafe extern "C" fn(*mut c_void, u32, u8) -> i32 = std::mem::transmute(address);
        call(storage, APP, 1)
    };
    report(
        output,
        serde_json::json!({"event": "file_count", "value": file_count}),
    )?;
    read_fixture(output, storage, base, name, PAYLOAD)?;
    if file_count != 1 {
        return Err("Expected exactly one indexed fixture".into());
    }
    Ok(())
}

fn load_cache(
    output: &mut std::fs::File,
    storage: *mut c_void,
    base: usize,
    engine: &abi::Engine,
    pipe: i32,
) -> Result<()> {
    let address = method(storage, base, 70, 0x902cb4)?;
    // SAFETY: pinned proxy sends one AppID scalar and has no return payload.
    unsafe {
        let call: unsafe extern "C" fn(*mut c_void, u32) = std::mem::transmute(address);
        call(storage, 480);
    }
    report(output, serde_json::json!({"event": "cache_load_requested"}))?;
    // This bounded callback window is not a completion contract. The caller's
    // actual read assertions determine whether the fixture became visible.
    let start = std::time::Instant::now();
    let mut callbacks = 0;
    while start.elapsed() < std::time::Duration::from_secs(2) {
        if engine.callback(pipe).is_some() {
            callbacks += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    report(
        output,
        serde_json::json!({"event": "cache_observation_window", "milliseconds": 2000, "callbacks": callbacks}),
    )
}

fn read_fixture(
    output: &mut std::fs::File,
    storage: *mut c_void,
    base: usize,
    name: &std::ffi::CStr,
    payload: &[u8],
) -> Result<()> {
    let address = method(storage, base, 5, 0x8fbd48)?;
    let mut bytes = [0_u8; 256];
    // SAFETY: same verified argument layout as FileWrite; output is bounded by
    // this buffer's length. The returned count is checked before slicing.
    let count = unsafe {
        let call: unsafe extern "C" fn(
            *mut c_void,
            u32,
            u32,
            *const std::ffi::c_char,
            *mut u8,
            u32,
        ) -> i32 = std::mem::transmute(address);
        call(
            storage,
            480,
            0,
            name.as_ptr(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
        )
    };
    let matched = count == payload.len() as i32 && bytes.get(..count as usize) == Some(payload);
    report(
        output,
        serde_json::json!({"event": "file_read", "name": name.to_str()?, "bytes": count, "matches_fixture": matched}),
    )?;
    if !matched {
        return Err("Offline fixture read did not match".into());
    }
    Ok(())
}

fn inventory(object: *mut c_void, base: usize, count: usize) -> Result<Vec<serde_json::Value>> {
    let mut methods = Vec::new();
    // SAFETY: only the interfaces obtained above reach this helper. Their
    // verified vtables have at least count entries; no method is invoked here.
    unsafe {
        let table = *object.cast::<*const *const c_void>();
        if table.is_null() {
            return Err("Null client vtable".into());
        }
        for slot in 0..count {
            let address = *table.add(slot);
            let mut info: libc::Dl_info = std::mem::zeroed();
            if libc::dladdr(address, &mut info) == 0 || info.dli_fbase as usize != base {
                return Err("Unexpected client vtable image".into());
            }
            methods.push(serde_json::json!({"slot": slot, "offset": format!("0x{:x}", address as usize - base)}));
        }
    }
    Ok(methods)
}

fn method(object: *mut c_void, base: usize, slot: usize, offset: usize) -> Result<*const c_void> {
    let entry = &inventory(object, base, slot + 1)?[slot];
    if entry["offset"] != format!("0x{offset:x}") {
        return Err("Unrecognized Cloud probe ABI; refusing call".into());
    }
    // SAFETY: inventory checked this entry and its owning image above.
    Ok(unsafe { *(*object.cast::<*const *const c_void>()).add(slot) })
}

fn report(output: &mut std::fs::File, value: serde_json::Value) -> Result<()> {
    serde_json::to_writer(&mut *output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
